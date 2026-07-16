FROM sysg-usecase-base

COPY tests/docker/usecase/abuse-double-register/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/abuse-double-register/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
